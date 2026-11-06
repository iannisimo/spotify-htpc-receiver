//! `htpc-nowplaying soloist` against a fake `soloist` script.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Env {
    dir: PathBuf,
}

impl Env {
    /// A temp `XDG_CONFIG_HOME` holding a config that points at a fake Soloist running `script`,
    /// and (unless `key` is `None`) an API key file.
    fn new(name: &str, script: &str, key: Option<&str>) -> Env {
        let dir = std::env::temp_dir().join(format!("htpc-launcher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("htpc-nowplaying")).unwrap();
        std::fs::create_dir_all(dir.join("soloist")).unwrap();

        let fake = dir.join("fake-soloist");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            dir.join("htpc-nowplaying/config.toml"),
            format!(
                "[daemon]\ndevice_name = \"Den\"\nsoloist_path = \"{}\"\n",
                fake.display()
            ),
        )
        .unwrap();
        if let Some(key) = key {
            std::fs::write(dir.join("soloist/api-key"), key).unwrap();
        }
        Env { dir }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_htpc-nowplaying"));
        cmd.arg("soloist")
            .args(args)
            .env("XDG_CONFIG_HOME", &self.dir)
            .env("HOME", &self.dir)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const ECHO_ARGS: &str = "#!/bin/sh\necho \"$$\"\nfor a in \"$@\"; do echo \"$a\"; done\n";

fn lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn exec_replaces_process() {
    let env = Env::new("exec", ECHO_ARGS, Some("  s3cret\n"));
    let child = env
        .command(&[])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let lines = lines(&output);
    assert_eq!(
        lines[0],
        pid.to_string(),
        "Soloist runs as the same process"
    );
    assert_eq!(
        lines[1..],
        [
            "--device-name",
            "Den",
            "--ws",
            "127.0.0.1:9090",
            "--api-key",
            "s3cret"
        ]
    );
}

#[test]
fn exit_code_passes_through() {
    let env = Env::new("exit", "#!/bin/sh\nexit 10\n", Some("k"));
    assert_eq!(env.run(&[]).status.code(), Some(10));
}

#[test]
fn missing_key_file_is_clear_error() {
    let env = Env::new("nokey", ECHO_ARGS, None);
    let output = env.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let key_path = env.dir.join("soloist/api-key");
    assert!(stderr.contains(key_path.to_str().unwrap()), "{stderr}");
}

#[test]
fn missing_binary_is_clear_error() {
    let env = Env::new("nobin", ECHO_ARGS, Some("k"));
    std::fs::remove_file(env.dir.join("fake-soloist")).unwrap();
    let output = env.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("fake-soloist"));
}

#[test]
fn extra_args_pass_through() {
    let env = Env::new("extra", ECHO_ARGS, Some("k"));
    let output = env.run(&["--", "-v"]);
    assert!(output.status.success());
    assert_eq!(lines(&output).last().map(String::as_str), Some("-v"));
}

#[test]
fn non_loopback_address_is_refused() {
    let env = Env::new("lan", ECHO_ARGS, Some("k"));
    let config = env.dir.join("htpc-nowplaying/config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        text.replace("[daemon]", "[daemon]\naddress = \"0.0.0.0:9090\""),
    )
    .unwrap();
    let output = env.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(lines(&output).is_empty(), "Soloist must not start");
}
