//! Pairing: keeping Soloist's zeroconf port open on the local network while it runs.
//!
//! The firewall is changed by a root-owned helper, run as `sudo -n <HELPER> open|close`. This
//! module never blocks on a password prompt (`-n`) and callers run [`run_helper`] off the UI
//! thread. Helper contract:
//!
//! - `open` opens the port Soloist listens on, for the local subnet only; `close` removes
//!   what `open` added. Both print `backend=<ufw|firewalld|none> state=<open|closed> port=<n|->`.
//! - Exit codes: 0 ok; 2 usage or refused; 3 no Soloist listener of this user; 4 unsupported
//!   firewall (stdout still has `port=<n>`); 5 a firewall command failed.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::{Config, PairingMode};
use crate::model::PlayerEvent;

/// Where the helper is installed. Distribution packages set `HTPC_PAIRING_HELPER` at build time
/// (the Arch package uses `/usr/lib/htpc-nowplaying/pairing-port`); the default matches
/// `install-pairing-helper.sh`.
pub const HELPER: &str = match option_env!("HTPC_PAIRING_HELPER") {
    Some(path) => path,
    None => "/usr/local/libexec/htpc-nowplaying/pairing-port",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperCommand {
    Open,
    Close,
}

impl HelperCommand {
    fn arg(self) -> &'static str {
        match self {
            HelperCommand::Open => "open",
            HelperCommand::Close => "close",
        }
    }
}

/// What the helper reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperStatus {
    Open {
        port: u16,
    },
    Closed,
    /// No firewall is active: nothing to open.
    NoFirewall,
    /// The helper is missing, or sudo wants a password or refuses.
    NotInstalled,
    /// Soloist is not listening yet (or not as this user).
    NoSoloistListener,
    /// A firewall the helper cannot drive; the port, if known, is to be opened by hand.
    Unsupported {
        port: Option<u16>,
    },
    Failed(String),
}

/// Runs the helper through `sudo -n`. Blocking.
pub fn run_helper(cmd: HelperCommand) -> HelperStatus {
    if !Path::new(HELPER).exists() {
        return HelperStatus::NotInstalled;
    }
    let output = Command::new("sudo")
        .args(["-n", HELPER, cmd.arg()])
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(out) => map_output(
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(e) => HelperStatus::Failed(format!("cannot run sudo: {e}")),
    }
}

/// Turns the helper's exit code and output into a status.
fn map_output(code: Option<i32>, stdout: &str, stderr: &str) -> HelperStatus {
    if code != Some(0) && sudo_refused(stderr) {
        return HelperStatus::NotInstalled;
    }
    match code {
        Some(0) => parse_status_line(stdout).unwrap_or_else(|| {
            HelperStatus::Failed("the helper's answer was not understood".into())
        }),
        Some(3) => HelperStatus::NoSoloistListener,
        Some(4) => HelperStatus::Unsupported {
            port: field(stdout, "port").and_then(|p| p.parse().ok()),
        },
        Some(n) => {
            let why = stderr.trim();
            HelperStatus::Failed(if why.is_empty() {
                format!("the helper exited with code {n}")
            } else {
                why.to_owned()
            })
        }
        None => HelperStatus::Failed("the helper was stopped".into()),
    }
}

/// sudo's own refusals: no rule for this user, or a password would be needed.
fn sudo_refused(stderr: &str) -> bool {
    [
        "password is required",
        "not allowed",
        "not in the sudoers",
        "may not run sudo",
    ]
    .iter()
    .any(|s| stderr.contains(s))
}

fn parse_status_line(stdout: &str) -> Option<HelperStatus> {
    let line = stdout.lines().find(|l| l.contains("backend="))?;
    if field(line, "backend") == Some("none") {
        return Some(HelperStatus::NoFirewall);
    }
    match field(line, "state")? {
        "open" => Some(HelperStatus::Open {
            port: field(line, "port")?.parse().ok()?,
        }),
        "closed" => Some(HelperStatus::Closed),
        _ => None,
    }
}

/// The value of `key=value` in a whitespace-separated line.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split_whitespace()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
}

/// The pairing toggle and what the helper last said.
#[derive(Debug)]
pub struct Pairing {
    mode: PairingMode,
    status: HelperStatus,
}

impl Pairing {
    pub fn new(mode: PairingMode) -> Pairing {
        Pairing {
            mode,
            status: HelperStatus::Closed,
        }
    }

    pub fn mode(&self) -> PairingMode {
        self.mode
    }

    /// The panel's toggle. What the helper should do now, if anything.
    pub fn set_mode(&mut self, mode: PairingMode) -> Option<HelperCommand> {
        if mode == self.mode {
            return None;
        }
        self.mode = mode;
        Some(match mode {
            PairingMode::On => HelperCommand::Open,
            PairingMode::Off => HelperCommand::Close,
        })
    }

    /// A Soloist connection change. Only matters for a Soloist started by hand: when the
    /// systemd unit runs it, the unit opens and closes the port itself, and doing it here too
    /// would race the unit.
    pub fn on_event(&mut self, ev: &PlayerEvent, unit_active: bool) -> Option<HelperCommand> {
        if unit_active || self.mode != PairingMode::On {
            return None;
        }
        match ev {
            PlayerEvent::Connected => Some(HelperCommand::Open),
            PlayerEvent::Disconnected => Some(HelperCommand::Close),
            _ => None,
        }
    }

    pub fn set_status(&mut self, status: HelperStatus) {
        self.status = status;
    }

    /// The line the panel shows under the Pairing row.
    pub fn status_line(&self) -> String {
        let on = self.mode == PairingMode::On;
        match &self.status {
            HelperStatus::NotInstalled => "Pairing helper not installed: see README".to_owned(),
            HelperStatus::Unsupported { port: Some(port) } => {
                format!("Firewall not supported: open port {port} by hand")
            }
            HelperStatus::Unsupported { port: None } => {
                "Firewall not supported: open the port by hand".to_owned()
            }
            HelperStatus::Failed(e) => format!("Could not change the firewall: {e}"),
            _ if !on => "Off".to_owned(),
            HelperStatus::Open { port } => {
                format!("On: port {port} open to the local network")
            }
            HelperStatus::NoSoloistListener => "On: waiting for Soloist".to_owned(),
            HelperStatus::NoFirewall => "No firewall: nothing to open".to_owned(),
            HelperStatus::Closed => "On".to_owned(),
        }
    }
}

/// `htpc-nowplaying pairing-sync`, run by the Soloist unit once Soloist has started: opens the
/// port when pairing is On. With Off it does nothing (the unit's `ExecStartPre` and
/// `ExecStopPost` already closed anything left over).
pub fn pairing_sync(cfg: &Config, helper: impl Fn(HelperCommand) -> HelperStatus) -> HelperStatus {
    match cfg.daemon.pairing {
        PairingMode::On => helper(HelperCommand::Open),
        PairingMode::Off => HelperStatus::Closed,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn set_mode_opens_and_closes() {
        let mut p = Pairing::new(PairingMode::Off);
        assert_eq!(p.set_mode(PairingMode::Off), None);
        assert_eq!(p.set_mode(PairingMode::On), Some(HelperCommand::Open));
        assert_eq!(p.set_mode(PairingMode::On), None);
        assert_eq!(p.set_mode(PairingMode::Off), Some(HelperCommand::Close));
        assert_eq!(p.mode(), PairingMode::Off);
    }

    #[test]
    fn connect_opens_only_when_unit_inactive() {
        let mut p = Pairing::new(PairingMode::On);
        assert_eq!(
            p.on_event(&PlayerEvent::Connected, false),
            Some(HelperCommand::Open)
        );
        assert_eq!(
            p.on_event(&PlayerEvent::Connected, true),
            None,
            "the unit's ExecStartPost owns the rule"
        );
    }

    #[test]
    fn disconnect_closes_only_when_unit_inactive() {
        let mut p = Pairing::new(PairingMode::On);
        assert_eq!(
            p.on_event(&PlayerEvent::Disconnected, false),
            Some(HelperCommand::Close)
        );
        assert_eq!(p.on_event(&PlayerEvent::Disconnected, true), None);
    }

    #[test]
    fn off_ignores_events() {
        let mut p = Pairing::new(PairingMode::Off);
        for unit_active in [false, true] {
            assert_eq!(p.on_event(&PlayerEvent::Connected, unit_active), None);
            assert_eq!(p.on_event(&PlayerEvent::Disconnected, unit_active), None);
        }
    }

    #[test]
    fn other_events_do_nothing() {
        let mut p = Pairing::new(PairingMode::On);
        assert_eq!(p.on_event(&PlayerEvent::DeviceActive(true), false), None);
    }

    #[test]
    fn pairing_sync_calls_open_only_when_on() {
        let calls = RefCell::new(Vec::new());
        let helper = |c| {
            calls.borrow_mut().push(c);
            HelperStatus::Open { port: 4242 }
        };
        let mut cfg = Config::default();
        assert_eq!(pairing_sync(&cfg, helper), HelperStatus::Closed);
        assert!(calls.borrow().is_empty());
        cfg.daemon.pairing = PairingMode::On;
        assert_eq!(
            pairing_sync(&cfg, helper),
            HelperStatus::Open { port: 4242 }
        );
        assert_eq!(*calls.borrow(), [HelperCommand::Open]);
    }

    #[test]
    fn status_lines() {
        let mut p = Pairing::new(PairingMode::Off);
        assert_eq!(p.status_line(), "Off");
        p.set_status(HelperStatus::Closed);
        assert_eq!(p.status_line(), "Off");

        p.set_mode(PairingMode::On);
        let line = |p: &mut Pairing, s| {
            p.set_status(s);
            p.status_line()
        };
        assert_eq!(
            line(&mut p, HelperStatus::Open { port: 51117 }),
            "On: port 51117 open to the local network"
        );
        assert_eq!(
            line(&mut p, HelperStatus::NoSoloistListener),
            "On: waiting for Soloist"
        );
        assert_eq!(
            line(&mut p, HelperStatus::NotInstalled),
            "Pairing helper not installed: see README"
        );
        assert_eq!(
            line(&mut p, HelperStatus::Unsupported { port: Some(51117) }),
            "Firewall not supported: open port 51117 by hand"
        );
        assert_eq!(
            line(&mut p, HelperStatus::Unsupported { port: None }),
            "Firewall not supported: open the port by hand"
        );
        assert_eq!(
            line(&mut p, HelperStatus::NoFirewall),
            "No firewall: nothing to open"
        );
        assert_eq!(
            line(&mut p, HelperStatus::Failed("ufw broke".into())),
            "Could not change the firewall: ufw broke"
        );
        assert_eq!(line(&mut p, HelperStatus::Closed), "On");

        // Switching Off hides a stale "open" but not a problem the user must see.
        p.set_mode(PairingMode::Off);
        assert_eq!(line(&mut p, HelperStatus::Open { port: 1 }), "Off");
        assert_eq!(
            line(&mut p, HelperStatus::NotInstalled),
            "Pairing helper not installed: see README"
        );
    }

    #[test]
    fn helper_output_is_mapped() {
        let ok = |out| map_output(Some(0), out, "");
        assert_eq!(
            ok("backend=ufw state=open port=51117\n"),
            HelperStatus::Open { port: 51117 }
        );
        assert_eq!(
            ok("backend=ufw state=closed port=-\n"),
            HelperStatus::Closed
        );
        assert_eq!(
            ok("backend=firewalld state=closed port=-"),
            HelperStatus::Closed
        );
        assert_eq!(
            ok("backend=none state=closed port=-"),
            HelperStatus::NoFirewall
        );
        assert!(matches!(ok("nonsense"), HelperStatus::Failed(_)));

        assert_eq!(map_output(Some(3), "", ""), HelperStatus::NoSoloistListener);
        assert_eq!(
            map_output(Some(4), "backend=unsupported state=closed port=51117\n", ""),
            HelperStatus::Unsupported { port: Some(51117) }
        );
        assert_eq!(
            map_output(Some(4), "", ""),
            HelperStatus::Unsupported { port: None }
        );
        assert_eq!(
            map_output(Some(5), "", "ufw: ERROR\n"),
            HelperStatus::Failed("ufw: ERROR".into())
        );
        assert_eq!(
            map_output(Some(2), "", ""),
            HelperStatus::Failed("the helper exited with code 2".into())
        );
        assert!(matches!(map_output(None, "", ""), HelperStatus::Failed(_)));
    }

    #[test]
    fn sudo_needs_password_is_not_installed() {
        for stderr in [
            "sudo: a password is required\n",
            "sudo: sorry, user simone is not allowed to execute '/x' as root\n",
            "simone is not in the sudoers file.\n",
            "Sorry, user simone may not run sudo on host.\n",
        ] {
            assert_eq!(
                map_output(Some(1), "", stderr),
                HelperStatus::NotInstalled,
                "{stderr}"
            );
        }
        // A helper that ran and failed is not "not installed", even if its message is odd.
        assert_eq!(
            map_output(Some(0), "backend=ufw state=closed port=-", "not allowed"),
            HelperStatus::Closed
        );
    }
}
