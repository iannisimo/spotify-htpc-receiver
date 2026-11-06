//! User configuration: `$XDG_CONFIG_HOME/htpc-nowplaying/config.toml`.
//!
//! Loading never fails. A missing file gives the defaults; an unreadable or invalid file gives the
//! defaults plus a warning; unknown keys are ignored with a warning; out-of-range values are
//! clamped with a warning.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::input::{Key, key_from_name};

mod save;
pub use save::{SaveError, Setting, save_setting};

/// Longest speaker name Spotify's device list takes comfortably.
const NAME_MAX: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Ask the compositor for a fullscreen window. Some ignore the request by policy (for
    /// example a Hyprland window rule); `false` gives a normal window, handy for development.
    pub fullscreen: bool,
    /// Up/Down change Spotify volume on the now-playing screen.
    pub audio_controls: bool,
    /// Volume change per Up/Down press, in percent (1–25).
    pub volume_step: u8,
    pub daemon: DaemonConfig,
    pub visualizer: VisualizerConfig,
    pub keys: KeyMap,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DaemonConfig {
    /// Soloist's WebSocket address, as passed to its `--ws` flag.
    pub address: String,
    /// Keep a firewall port open for pairing while Soloist runs.
    pub pairing: PairingMode,
    /// The speaker name as written in the file (validated where used); `None` means
    /// [`default_device_name`].
    pub device_name: Option<String>,
    /// The Soloist binary `htpc-nowplaying soloist` starts.
    pub soloist_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PairingMode {
    #[default]
    Off,
    On,
}

impl PairingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PairingMode::Off => "off",
            PairingMode::On => "on",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    BadChar(char),
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NameError::Empty => f.write_str("Name is empty"),
            NameError::TooLong => write!(f, "Name is too long ({NAME_MAX} max)"),
            NameError::BadChar(c) => write!(f, "'{c}' can't be used"),
        }
    }
}

impl std::error::Error for NameError {}

/// Checks a speaker name and returns it trimmed: 1–32 characters of letters, digits, space and
/// `- _ . '`.
pub fn validate_device_name(raw: &str) -> Result<String, NameError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '\'')))
    {
        return Err(NameError::BadChar(c));
    }
    if name.chars().count() > NAME_MAX {
        return Err(NameError::TooLong);
    }
    Ok(name.to_owned())
}

/// `soloist-` and the short hostname, or `soloist-htpc` when the hostname is unknown.
pub fn default_device_name() -> String {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    default_name_for_host(&host)
}

fn default_name_for_host(hostname: &str) -> String {
    let short = hostname.trim().split('.').next().unwrap_or("");
    let short = if short.is_empty() { "htpc" } else { short };
    // Hostnames are letters, digits and hyphens; anything else would fail validation.
    let cleaned: String = short
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("soloist-{cleaned}")
        .chars()
        .take(NAME_MAX)
        .collect()
}

/// The name Soloist is started with: the configured one, or the default when absent or invalid
/// (invalid also logs a warning).
pub fn effective_device_name(cfg: &DaemonConfig) -> String {
    let (name, error) = resolve_device_name(cfg.device_name.as_deref(), &default_device_name());
    if let Some(e) = error {
        log::warn!("`daemon.device_name`: {e}; using {name}");
    }
    name
}

fn resolve_device_name(configured: Option<&str>, default: &str) -> (String, Option<NameError>) {
    match configured.map(validate_device_name) {
        None => (default.to_owned(), None),
        Some(Ok(name)) => (name, None),
        Some(Err(e)) => (default.to_owned(), Some(e)),
    }
}

/// `$XDG_CONFIG_HOME/htpc-nowplaying/disabled-presets.txt`, next to `config.toml`.
pub fn disabled_presets_path() -> PathBuf {
    let config = default_path();
    config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("disabled-presets.txt")
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisualizerConfig {
    pub preset_dir: Option<PathBuf>,
    pub preset_duration_s: f64,
    pub shuffle: bool,
    pub hard_cuts: bool,
    pub soft_cut_s: f64,
    pub mesh_width: u32,
    pub mesh_height: u32,
    pub fps_cap: u32,
    /// Lowest render scale adaptive quality may drop to (0.1–1.0).
    pub min_scale: f32,
    /// Lowest frame rate adaptive quality may drop to (5–`fps_cap`).
    pub min_fps: u32,
    pub beat_sensitivity: f32,
    /// Delay applied to captured audio so visuals line up with what is heard.
    pub delay_ms: u32,
}

/// Maps physical key names (keysym names as shown by `wev`, e.g. `Return`, `Escape`, `a`) to
/// remote buttons. Usable keys: arrows, Return (also the keypad Enter), Escape, BackSpace, Tab,
/// space, Delete, Insert, Home, End, Page_Up, Page_Down, Menu, Pause, F1–F24, printable
/// characters, and the remote keys XF86Back, XF86Forward and XF86HomePage. Media keys
/// (XF86AudioPlay and the like) are not delivered.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyMap(HashMap<String, Key>);

impl KeyMap {
    pub fn get(&self, key_name: &str) -> Option<Key> {
        self.0.get(key_name).copied()
    }
}

impl Default for KeyMap {
    fn default() -> Self {
        let bindings = [
            ("Left", Key::Left),
            ("Right", Key::Right),
            ("Up", Key::Up),
            ("Down", Key::Down),
            ("Return", Key::Ok),
            ("Escape", Key::Back),
            ("BackSpace", Key::Back),
            ("XF86Back", Key::Back),
            ("Menu", Key::Settings),
        ];
        KeyMap(
            bindings
                .iter()
                .map(|&(name, key)| (name.to_owned(), key))
                .collect(),
        )
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            fullscreen: true,
            audio_controls: false,
            volume_step: 5,
            daemon: DaemonConfig {
                address: "127.0.0.1:9090".to_owned(),
                pairing: PairingMode::Off,
                device_name: None,
                soloist_path: expand_tilde(DEFAULT_SOLOIST_PATH, std::env::home_dir().as_deref()),
            },
            visualizer: VisualizerConfig {
                preset_dir: None,
                preset_duration_s: 30.0,
                shuffle: true,
                hard_cuts: false,
                soft_cut_s: 3.0,
                mesh_width: 48,
                mesh_height: 32,
                fps_cap: 60,
                min_scale: 0.5,
                min_fps: 30,
                beat_sensitivity: 1.0,
                delay_ms: 0,
            },
            keys: KeyMap::default(),
        }
    }
}

const APP_DIR: &str = "htpc-nowplaying";
const DEFAULT_SOLOIST_PATH: &str = "~/.local/opt/soloist/soloist";

pub fn default_path() -> PathBuf {
    default_path_from(std::env::var_os("XDG_CONFIG_HOME"), std::env::home_dir())
}

/// `$XDG_CONFIG_HOME` if set to an absolute path, else `~/.config` (else `.config` without a home).
fn default_path_from(xdg_config_home: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    let base = xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join(APP_DIR).join("config.toml")
}

pub fn load(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            log::warn!("cannot read {}: {e}; using defaults", path.display());
            return Config::default();
        }
    };
    let (config, warnings) = parse(&text);
    for w in warnings {
        log::warn!("{}: {w}", path.display());
    }
    config
}

/// Parses config text. Returns the config and human-readable warnings for everything ignored or adjusted.
fn parse(text: &str) -> (Config, Vec<String>) {
    parse_with_home(text, std::env::home_dir().as_deref())
}

fn parse_with_home(text: &str, home: Option<&Path>) -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    let raw = toml::Deserializer::parse(text).and_then(|de| {
        serde_ignored::deserialize(de, |path| {
            warnings.push(format!("unknown config key `{path}` ignored"));
        })
    });
    match raw {
        Ok(raw) => {
            let config = validate(raw, home, &mut warnings);
            (config, warnings)
        }
        Err(e) => (
            Config::default(),
            vec![format!(
                "invalid config, using defaults: {}",
                e.to_string().trim_end()
            )],
        ),
    }
}

// Raw file shape. Numbers are read as i64/f64 so that out-of-range values can be clamped
// instead of failing the whole file.

#[derive(Deserialize)]
#[serde(default)]
struct RawConfig {
    fullscreen: bool,
    audio_controls: bool,
    volume_step: i64,
    daemon: RawDaemon,
    visualizer: RawVisualizer,
    keys: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(default)]
struct RawDaemon {
    address: String,
    pairing: String,
    device_name: Option<String>,
    soloist_path: String,
}

#[derive(Deserialize)]
#[serde(default)]
struct RawVisualizer {
    preset_dir: Option<String>,
    preset_duration_s: f64,
    shuffle: bool,
    hard_cuts: bool,
    soft_cut_s: f64,
    mesh_width: i64,
    mesh_height: i64,
    fps_cap: i64,
    min_scale: f64,
    min_fps: i64,
    beat_sensitivity: f64,
    delay_ms: i64,
}

impl Default for RawConfig {
    fn default() -> Self {
        let d = Config::default();
        let v = d.visualizer;
        RawConfig {
            fullscreen: d.fullscreen,
            audio_controls: d.audio_controls,
            volume_step: d.volume_step.into(),
            daemon: RawDaemon {
                address: d.daemon.address,
                pairing: d.daemon.pairing.as_str().to_owned(),
                device_name: None,
                soloist_path: DEFAULT_SOLOIST_PATH.to_owned(),
            },
            visualizer: RawVisualizer {
                preset_dir: None,
                preset_duration_s: v.preset_duration_s,
                shuffle: v.shuffle,
                hard_cuts: v.hard_cuts,
                soft_cut_s: v.soft_cut_s,
                mesh_width: v.mesh_width.into(),
                mesh_height: v.mesh_height.into(),
                fps_cap: v.fps_cap.into(),
                min_scale: v.min_scale.into(),
                min_fps: v.min_fps.into(),
                beat_sensitivity: v.beat_sensitivity.into(),
                delay_ms: v.delay_ms.into(),
            },
            keys: BTreeMap::new(),
        }
    }
}

impl Default for RawDaemon {
    fn default() -> Self {
        RawConfig::default().daemon
    }
}

impl Default for RawVisualizer {
    fn default() -> Self {
        RawConfig::default().visualizer
    }
}

fn validate(raw: RawConfig, home: Option<&Path>, w: &mut Vec<String>) -> Config {
    let d = Config::default();
    let rv = raw.visualizer;
    let dv = d.visualizer;

    let fps_cap = clamp_int(w, "visualizer.fps_cap", rv.fps_cap, 10, 240);
    let visualizer = VisualizerConfig {
        preset_dir: rv
            .preset_dir
            .filter(|s| !s.is_empty())
            .map(|s| expand_tilde(&s, home)),
        preset_duration_s: clamp_float(
            w,
            "visualizer.preset_duration_s",
            rv.preset_duration_s,
            (1.0, 3600.0),
            dv.preset_duration_s,
        ),
        shuffle: rv.shuffle,
        hard_cuts: rv.hard_cuts,
        soft_cut_s: clamp_float(
            w,
            "visualizer.soft_cut_s",
            rv.soft_cut_s,
            (0.0, 60.0),
            dv.soft_cut_s,
        ),
        mesh_width: clamp_int(w, "visualizer.mesh_width", rv.mesh_width, 8, 512),
        mesh_height: clamp_int(w, "visualizer.mesh_height", rv.mesh_height, 8, 512),
        fps_cap,
        min_scale: clamp_float(
            w,
            "visualizer.min_scale",
            rv.min_scale,
            (0.1, 1.0),
            dv.min_scale.into(),
        ) as f32,
        min_fps: clamp_int(w, "visualizer.min_fps", rv.min_fps, 5, fps_cap.into()),
        beat_sensitivity: clamp_float(
            w,
            "visualizer.beat_sensitivity",
            rv.beat_sensitivity,
            (0.0, 5.0),
            dv.beat_sensitivity.into(),
        ) as f32,
        delay_ms: clamp_int(w, "visualizer.delay_ms", rv.delay_ms, 0, 2000),
    };

    let address = if is_host_port(&raw.daemon.address) {
        raw.daemon.address
    } else {
        w.push(format!(
            "`daemon.address` = {:?} is not host:port; using {}",
            raw.daemon.address, d.daemon.address
        ));
        d.daemon.address
    };

    let pairing = match raw.daemon.pairing.as_str() {
        "on" => PairingMode::On,
        "off" => PairingMode::Off,
        other => {
            w.push(format!(
                "`daemon.pairing` = {other:?} is not \"on\" or \"off\"; using \"off\""
            ));
            PairingMode::Off
        }
    };
    let soloist_path = if raw.daemon.soloist_path.is_empty() {
        d.daemon.soloist_path
    } else {
        expand_tilde(&raw.daemon.soloist_path, home)
    };

    Config {
        fullscreen: raw.fullscreen,
        audio_controls: raw.audio_controls,
        volume_step: clamp_int(w, "volume_step", raw.volume_step, 1, 25),
        daemon: DaemonConfig {
            address,
            pairing,
            device_name: raw.daemon.device_name,
            soloist_path,
        },
        visualizer,
        keys: apply_key_remap(raw.keys, w),
    }
}

fn clamp_int<T>(w: &mut Vec<String>, name: &str, value: i64, lo: i64, hi: i64) -> T
where
    T: TryFrom<i64>,
    T::Error: std::fmt::Debug,
{
    let clamped = value.clamp(lo, hi);
    if clamped != value {
        w.push(format!(
            "`{name}` = {value} is outside {lo}..={hi}; using {clamped}"
        ));
    }
    T::try_from(clamped).expect("range bounds fit the target type")
}

fn clamp_float(
    w: &mut Vec<String>,
    name: &str,
    value: f64,
    (lo, hi): (f64, f64),
    default: f64,
) -> f64 {
    if !value.is_finite() {
        w.push(format!(
            "`{name}` = {value} is not a finite number; using {default}"
        ));
        return default;
    }
    let clamped = value.clamp(lo, hi);
    if clamped != value {
        w.push(format!(
            "`{name}` = {value} is outside {lo}..={hi}; using {clamped}"
        ));
    }
    clamped
}

fn is_host_port(address: &str) -> bool {
    match address.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p != 0),
        None => false,
    }
}

fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(path),
    }
}

fn apply_key_remap(remap: BTreeMap<String, String>, w: &mut Vec<String>) -> KeyMap {
    let mut keys = KeyMap::default();
    for (key_name, action) in remap {
        if action.eq_ignore_ascii_case("none") {
            keys.0.remove(&key_name);
        } else if let Some(key) = key_from_name(&action) {
            keys.0.insert(key_name, key);
        } else {
            w.push(format!(
                "`keys.{key_name}` = {action:?} is not one of left, right, up, down, ok, back, settings, none; ignored"
            ));
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/config")
            .join(name)
    }

    fn parse_fixture(name: &str) -> (Config, Vec<String>) {
        parse(&std::fs::read_to_string(fixture(name)).unwrap())
    }

    #[test]
    fn missing_file_gives_defaults() {
        assert_eq!(load(&fixture("does_not_exist.toml")), Config::default());
    }

    #[test]
    fn empty_file_gives_defaults_without_warnings() {
        assert_eq!(parse(""), (Config::default(), vec![]));
    }

    #[test]
    fn audio_controls_defaults_false() {
        assert!(!Config::default().audio_controls);
        let (cfg, _) = parse("volume_step = 3\n");
        assert!(!cfg.audio_controls);
    }

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert!(cfg.fullscreen);
        assert_eq!(cfg.volume_step, 5);
        assert_eq!(cfg.daemon.address, "127.0.0.1:9090");
        let viz = &cfg.visualizer;
        assert_eq!(viz.preset_dir, None);
        assert_eq!(viz.delay_ms, 0);
        assert_eq!(viz.fps_cap, 60);
        assert!(viz.min_fps <= viz.fps_cap);
        assert_eq!(viz.beat_sensitivity, 1.0);
    }

    #[test]
    fn full_file_is_read() {
        let (cfg, warnings) = parse_fixture("full.toml");
        assert_eq!(warnings, Vec::<String>::new());
        assert!(!cfg.fullscreen);
        assert!(cfg.audio_controls);
        assert_eq!(cfg.volume_step, 10);
        assert_eq!(cfg.daemon.address, "127.0.0.1:9191");
        let expected = VisualizerConfig {
            preset_dir: Some(PathBuf::from("/usr/share/projectM/presets")),
            preset_duration_s: 20.0,
            shuffle: false,
            hard_cuts: true,
            soft_cut_s: 5.0,
            mesh_width: 64,
            mesh_height: 48,
            fps_cap: 30,
            min_scale: 0.25,
            min_fps: 15,
            beat_sensitivity: 2.5,
            delay_ms: 120,
        };
        assert_eq!(cfg.visualizer, expected);
    }

    #[test]
    fn invalid_toml_gives_defaults() {
        let (cfg, warnings) = parse_fixture("invalid.toml");
        assert_eq!(cfg, Config::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(load(&fixture("invalid.toml")), Config::default());
    }

    #[test]
    fn wrong_type_gives_defaults() {
        let (cfg, warnings) = parse("audio_controls = \"yes\"\n");
        assert_eq!(cfg, Config::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let (cfg, warnings) = parse_fixture("unknown_keys.toml");
        assert!(cfg.audio_controls);
        assert_eq!(cfg.daemon.address, "127.0.0.1:9191");
        assert_eq!(cfg.visualizer.fps_cap, 30);
        let mut warnings = warnings;
        warnings.sort();
        assert_eq!(
            warnings,
            [
                "unknown config key `colour_scheme` ignored",
                "unknown config key `daemon.api_token` ignored",
                "unknown config key `lyrics` ignored",
                "unknown config key `visualizer.bloom` ignored",
            ]
        );
    }

    #[test]
    fn out_of_range_values_clamped() {
        let (cfg, warnings) = parse_fixture("out_of_range.toml");
        let defaults = Config::default();
        let viz = &cfg.visualizer;
        assert_eq!(cfg.volume_step, 25);
        assert_eq!(cfg.daemon.address, defaults.daemon.address);
        assert_eq!(viz.preset_duration_s, defaults.visualizer.preset_duration_s);
        assert_eq!(viz.soft_cut_s, 0.0);
        assert_eq!(viz.mesh_width, 8);
        assert_eq!(viz.mesh_height, 512);
        assert_eq!(viz.fps_cap, 10);
        assert_eq!(viz.min_scale, 1.0);
        assert_eq!(viz.min_fps, 10, "min_fps is capped by the clamped fps_cap");
        assert_eq!(viz.beat_sensitivity, 0.0);
        assert_eq!(viz.delay_ms, 2000);
        assert_eq!(
            warnings.len(),
            11,
            "one warning per bad value: {warnings:?}"
        );
    }

    #[test]
    fn infinite_float_falls_back_to_default() {
        let (cfg, warnings) = parse("[visualizer]\nbeat_sensitivity = inf\n");
        assert_eq!(cfg.visualizer.beat_sensitivity, 1.0);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn key_remap_applies() {
        let (cfg, warnings) = parse_fixture("key_remap.toml");
        let keys = &cfg.keys;
        assert_eq!(keys.get("BackSpace"), Some(Key::Ok), "default overridden");
        assert_eq!(keys.get("space"), Some(Key::Ok), "binding added");
        assert_eq!(keys.get("Escape"), None, "`none` removes a binding");
        assert_eq!(keys.get("F12"), None, "invalid action skipped");
        assert_eq!(keys.get("Return"), Some(Key::Ok), "untouched defaults kept");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn default_keys_cover_a_remote() {
        let keys = KeyMap::default();
        assert_eq!(keys.get("Left"), Some(Key::Left));
        assert_eq!(keys.get("Right"), Some(Key::Right));
        assert_eq!(keys.get("Up"), Some(Key::Up));
        assert_eq!(keys.get("Down"), Some(Key::Down));
        assert_eq!(keys.get("Return"), Some(Key::Ok));
        assert_eq!(keys.get("Escape"), Some(Key::Back));
        assert_eq!(keys.get("BackSpace"), Some(Key::Back));
        assert_eq!(
            keys.get("left"),
            None,
            "key names are case-sensitive keysyms"
        );
    }

    #[test]
    fn preset_dir_tilde_is_expanded() {
        let (cfg, _) = parse_with_home(
            "[visualizer]\npreset_dir = \"~/presets\"\n",
            Some(Path::new("/home/me")),
        );
        assert_eq!(
            cfg.visualizer.preset_dir,
            Some(PathBuf::from("/home/me/presets"))
        );
    }

    /// The shipped example lists every setting commented out with its default. Uncommented,
    /// it must parse cleanly and give exactly the defaults (preset_dir aside, which has none).
    #[test]
    fn example_config_documents_the_defaults() {
        let example = include_str!("../../packaging/config.example.toml");
        let uncommented: String = example
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if rest.contains(" = ") && !rest.contains(':') => rest,
                _ => line,
            })
            .map(|line| format!("{line}\n"))
            .collect();
        // 3 top-level, 4 daemon, 12 visualizer, 9 default keys: a new setting belongs here too.
        let settings = uncommented.lines().filter(|l| l.contains(" = ")).count();
        assert_eq!(settings, 28);

        let (mut cfg, warnings) = parse_with_home(&uncommented, Some(Path::new("/home/me")));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            cfg.visualizer.preset_dir.take(),
            Some(PathBuf::from("/home/me/.local/share/projectM/presets"))
        );
        // These two have no fixed default (the name follows the hostname, the path the home
        // folder), so the example shows a sample value.
        assert_eq!(
            cfg.daemon.device_name.take().as_deref(),
            Some("soloist-myhost")
        );
        assert_eq!(
            std::mem::take(&mut cfg.daemon.soloist_path),
            PathBuf::from("/home/me/.local/opt/soloist/soloist")
        );
        cfg.daemon.soloist_path = Config::default().daemon.soloist_path;
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn pairing_mode_parses() {
        let (cfg, warnings) = parse("[daemon]\npairing = \"on\"\n");
        assert_eq!(cfg.daemon.pairing, PairingMode::On);
        assert!(warnings.is_empty(), "{warnings:?}");
        let (cfg, warnings) = parse("");
        assert_eq!(cfg.daemon.pairing, PairingMode::Off);
        assert!(warnings.is_empty());
        let (cfg, warnings) = parse("[daemon]\npairing = \"always\"\n");
        assert_eq!(cfg.daemon.pairing, PairingMode::Off);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn device_name_validation() {
        assert_eq!(
            validate_device_name("  Living room  "),
            Ok("Living room".to_owned())
        );
        assert_eq!(validate_device_name(""), Err(NameError::Empty));
        assert_eq!(validate_device_name("   "), Err(NameError::Empty));
        assert_eq!(
            validate_device_name(&"a".repeat(33)),
            Err(NameError::TooLong)
        );
        assert_eq!(validate_device_name(&"a".repeat(32)), Ok("a".repeat(32)));
        assert_eq!(validate_device_name("tv/1"), Err(NameError::BadChar('/')));
        assert!(validate_device_name("Kitchen's TV-2_b.x").is_ok());
        assert_eq!(NameError::Empty.to_string(), "Name is empty");
        assert_eq!(NameError::TooLong.to_string(), "Name is too long (32 max)");
        assert_eq!(NameError::BadChar('/').to_string(), "'/' can't be used");
    }

    #[test]
    fn default_device_name_shape() {
        let name = default_device_name();
        assert!(name.starts_with("soloist-"), "{name}");
        assert!(name.chars().count() <= 32);
        assert!(validate_device_name(&name).is_ok(), "{name}");
        assert_eq!(default_name_for_host("Den.local\n"), "soloist-Den");
        assert_eq!(default_name_for_host(""), "soloist-htpc");
        assert_eq!(default_name_for_host(&"h".repeat(60)).chars().count(), 32);
    }

    #[test]
    fn effective_name_falls_back() {
        assert_eq!(
            resolve_device_name(None, "soloist-x"),
            ("soloist-x".to_owned(), None)
        );
        assert_eq!(
            resolve_device_name(Some("x/y"), "soloist-x"),
            ("soloist-x".to_owned(), Some(NameError::BadChar('/')))
        );
        assert_eq!(
            resolve_device_name(Some("Den"), "soloist-x"),
            ("Den".to_owned(), None)
        );
        let mut daemon = Config::default().daemon;
        daemon.device_name = Some(" Den ".to_owned());
        assert_eq!(effective_device_name(&daemon), "Den");
    }

    #[test]
    fn settings_key_defaults_to_menu() {
        assert_eq!(KeyMap::default().get("Menu"), Some(Key::Settings));
    }

    #[test]
    fn remote_back_key_defaults_to_back() {
        assert_eq!(KeyMap::default().get("XF86Back"), Some(Key::Back));
    }

    #[test]
    fn soloist_path_is_expanded() {
        let (cfg, _) = parse_with_home(
            "[daemon]\nsoloist_path = \"~/bin/soloist\"\n",
            Some(Path::new("/home/me")),
        );
        assert_eq!(
            cfg.daemon.soloist_path,
            PathBuf::from("/home/me/bin/soloist")
        );
    }

    #[test]
    fn xdg_config_home_respected() {
        let path = default_path_from(
            Some(OsString::from("/xdg")),
            Some(PathBuf::from("/home/me")),
        );
        assert_eq!(path, PathBuf::from("/xdg/htpc-nowplaying/config.toml"));
    }

    #[test]
    fn falls_back_to_home_config() {
        let home = Some(PathBuf::from("/home/me"));
        let expected = PathBuf::from("/home/me/.config/htpc-nowplaying/config.toml");
        assert_eq!(default_path_from(None, home.clone()), expected);
        // The XDG spec says empty or relative values must be ignored.
        assert_eq!(
            default_path_from(Some(OsString::new()), home.clone()),
            expected
        );
        assert_eq!(
            default_path_from(Some(OsString::from("rel/dir")), home),
            expected
        );
    }
}
