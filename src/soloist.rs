//! Starting Soloist (`htpc-nowplaying soloist`) and talking to its systemd user unit.
//!
//! The launcher puts the speaker name from `config.toml` on Soloist's command line and then
//! replaces itself with Soloist (`exec`), so the process is `soloist` and its exit code (10 for
//! an expired build) reaches systemd unchanged.

use std::ffi::OsString;
use std::fmt;
use std::net::IpAddr;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::{DaemonConfig, default_path, effective_device_name};

#[derive(Debug, PartialEq, Eq)]
pub enum LaunchError {
    KeyMissing(PathBuf),
    KeyEmpty(PathBuf),
    NotLoopback(String),
    BinaryMissing(PathBuf),
    /// `exec` itself failed.
    Exec(String),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::KeyMissing(path) => write!(
                f,
                "No Soloist API key at {}: see README, Install Soloist",
                path.display()
            ),
            LaunchError::KeyEmpty(path) => write!(
                f,
                "The Soloist API key file {} is empty: see README, Install Soloist",
                path.display()
            ),
            LaunchError::NotLoopback(address) => write!(
                f,
                "daemon.address {address:?} is not a loopback address; Soloist's WebSocket has no \
                 authentication, so it must stay on 127.0.0.1"
            ),
            LaunchError::BinaryMissing(path) => write!(
                f,
                "Soloist not found at {}: run packaging/install-soloist.sh, or set \
                 daemon.soloist_path in config.toml",
                path.display()
            ),
            LaunchError::Exec(e) => write!(f, "could not start Soloist: {e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// `$XDG_CONFIG_HOME/soloist/api-key`, default `~/.config/soloist/api-key`.
pub fn api_key_path() -> PathBuf {
    default_path()
        .parent()
        .and_then(|app_dir| app_dir.parent())
        .map(|base| base.join("soloist/api-key"))
        .unwrap_or_else(|| PathBuf::from(".config/soloist/api-key"))
}

/// Soloist's arguments: `--device-name <name> --ws <address> --api-key <key>`.
pub fn launch_args(cfg: &DaemonConfig, api_key: &str) -> Result<Vec<OsString>, LaunchError> {
    if !is_loopback(&cfg.address) {
        return Err(LaunchError::NotLoopback(cfg.address.clone()));
    }
    Ok(vec![
        "--device-name".into(),
        effective_device_name(cfg).into(),
        "--ws".into(),
        cfg.address.clone().into(),
        "--api-key".into(),
        api_key.into(),
    ])
}

fn is_loopback(address: &str) -> bool {
    let Some((host, _port)) = address.rsplit_once(':') else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Replaces this process with Soloist, passing `extra` after the usual arguments. It only
/// returns on failure.
pub fn exec_soloist(cfg: &DaemonConfig, extra: &[OsString]) -> LaunchError {
    let key_path = api_key_path();
    let key = match std::fs::read_to_string(&key_path) {
        Ok(text) => text.trim().to_owned(),
        Err(_) => return LaunchError::KeyMissing(key_path),
    };
    if key.is_empty() {
        return LaunchError::KeyEmpty(key_path);
    }
    let mut args = match launch_args(cfg, &key) {
        Ok(args) => args,
        Err(e) => return e,
    };
    args.extend_from_slice(extra);
    if !cfg.soloist_path.is_file() {
        return LaunchError::BinaryMissing(cfg.soloist_path.clone());
    }
    let error = Command::new(&cfg.soloist_path).args(args).exec();
    LaunchError::Exec(error.to_string())
}

const UNIT: &str = "soloist.service";

/// Whether the Soloist user unit is running. Blocking; call it off the UI thread.
pub fn service_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", UNIT])
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn daemon() -> DaemonConfig {
        Config::default().daemon
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn launch_args_use_config_name_and_address() {
        let cfg = DaemonConfig {
            device_name: Some("Den".to_owned()),
            address: "127.0.0.1:9191".to_owned(),
            ..daemon()
        };
        assert_eq!(
            strings(launch_args(&cfg, "k3y").unwrap()),
            [
                "--device-name",
                "Den",
                "--ws",
                "127.0.0.1:9191",
                "--api-key",
                "k3y"
            ]
        );
    }

    #[test]
    fn launch_args_default_name() {
        let args = strings(launch_args(&daemon(), "k").unwrap());
        assert_eq!(args[0], "--device-name");
        assert_eq!(args[1], effective_device_name(&daemon()));
        assert!(args[1].starts_with("soloist-"));
    }

    #[test]
    fn refuses_non_loopback_address() {
        for address in ["0.0.0.0:9090", "10.0.0.5:9090", "example.com:9090"] {
            let cfg = DaemonConfig {
                address: address.to_owned(),
                ..daemon()
            };
            assert_eq!(
                launch_args(&cfg, "k"),
                Err(LaunchError::NotLoopback(address.to_owned()))
            );
        }
        for address in ["127.0.0.1:9090", "[::1]:9090", "localhost:9090"] {
            let cfg = DaemonConfig {
                address: address.to_owned(),
                ..daemon()
            };
            assert!(launch_args(&cfg, "k").is_ok(), "{address}");
        }
    }
}
