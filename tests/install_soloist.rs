//! `packaging/install-soloist.sh` against fake `curl`, `uname` and `systemctl`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Setup {
    root: PathBuf,
}

impl Setup {
    fn new(name: &str) -> Setup {
        let root = std::env::temp_dir().join(format!("htpc-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["fakes", "home", "download"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let setup = Setup { root };
        setup.script(
            "curl",
            // Copies the tarball to the --output path, unless told to fail.
            r#"#!/bin/sh
[ -e "$FAKES/curl-fails" ] && { echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; }
while [ $# -gt 0 ]; do
    [ "$1" = "--output" ] && out="$2"
    shift
done
cp "$FAKES/soloist.tar.gz" "$out"
"#,
        );
        setup.script("uname", "#!/bin/sh\necho x86_64\n");
        setup.script(
            "systemctl",
            // Logs its arguments; answers is-active / is-failed from marker files.
            r#"#!/bin/sh
echo "$*" >> "$FAKES/systemctl.log"
case "$2" in
    is-active) [ -e "$FAKES/active" ] ;;
    is-failed) [ -e "$FAKES/failed" ] ;;
    *) exit 0 ;;
esac
"#,
        );
        setup.tarball("1.0");
        setup
    }

    fn fakes(&self) -> PathBuf {
        self.root.join("fakes")
    }

    fn dest(&self) -> PathBuf {
        self.root.join("opt/soloist")
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.fakes().join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Makes the tarball the fake `curl` serves: a `soloist` that prints `soloist <version>`.
    fn tarball(&self, version: &str) {
        let dir = self.root.join("download");
        let soloist = dir.join("soloist");
        std::fs::write(&soloist, format!("#!/bin/sh\necho soloist {version}\n")).unwrap();
        std::fs::set_permissions(&soloist, std::fs::Permissions::from_mode(0o755)).unwrap();
        let status = Command::new("tar")
            .arg("-czf")
            .arg(self.fakes().join("soloist.tar.gz"))
            .arg("-C")
            .arg(&dir)
            .arg("soloist")
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn marker(&self, name: &str) {
        std::fs::write(self.fakes().join(name), "").unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/install-soloist.sh");
        Command::new("/bin/sh")
            .arg(script)
            .args(args)
            .arg(self.dest())
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.fakes().display()))
            .env("FAKES", self.fakes())
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn systemctl_log(&self) -> String {
        std::fs::read_to_string(self.fakes().join("systemctl.log")).unwrap_or_default()
    }

    fn installed_version(&self) -> String {
        let out = Command::new(self.dest().join("soloist")).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn install_copies_itself_and_stores_hash() {
    let s = Setup::new("install");
    let out = s.run(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.dest().join("install-soloist.sh").is_file());
    let hash = std::fs::read_to_string(s.dest().join(".tarball.sha256")).unwrap();
    assert_eq!(hash.trim().len(), 64, "{hash}");
    assert_eq!(s.installed_version(), "soloist 1.0");
    // A folder other than the default must be told to the app.
    assert!(text(&out).contains("soloist_path"), "{}", text(&out));
}

#[test]
fn update_is_noop_when_unchanged() {
    let s = Setup::new("noop");
    assert!(s.run(&[]).status.success());
    let before = std::fs::metadata(s.dest().join("soloist"))
        .unwrap()
        .modified()
        .unwrap();
    s.marker("active");

    let out = s.run(&["--update"]);
    assert!(out.status.success());
    assert!(
        text(&out).contains("Soloist is up to date"),
        "{}",
        text(&out)
    );
    let after = std::fs::metadata(s.dest().join("soloist"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "files untouched");
    assert!(
        !s.systemctl_log().contains("restart"),
        "{}",
        s.systemctl_log()
    );
}

#[test]
fn update_replaces_and_restarts_running_soloist() {
    let s = Setup::new("running");
    assert!(s.run(&[]).status.success());
    s.tarball("2.0");
    s.marker("active");

    let out = s.run(&["--update"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(s.installed_version(), "soloist 2.0");
    assert!(
        s.systemctl_log().contains("--user restart soloist.service"),
        "{}",
        s.systemctl_log()
    );
    assert!(
        text(&out).contains("Updated Soloist to soloist 2.0"),
        "{}",
        text(&out)
    );

    // The new hash is stored: the same build again is a no-op.
    assert!(text(&s.run(&["--update"])).contains("up to date"));
}

#[test]
fn update_restarts_failed_soloist() {
    // An expired build exits with 10 and the unit stays failed.
    let s = Setup::new("failed");
    assert!(s.run(&[]).status.success());
    s.tarball("2.0");
    s.marker("failed");
    assert!(s.run(&["--update"]).status.success());
    assert!(s.systemctl_log().contains("--user restart soloist.service"));
}

#[test]
fn update_leaves_stopped_soloist_alone() {
    let s = Setup::new("stopped");
    assert!(s.run(&[]).status.success());
    s.tarball("2.0");
    assert!(s.run(&["--update"]).status.success());
    assert_eq!(s.installed_version(), "soloist 2.0", "files are updated");
    assert!(
        !s.systemctl_log().contains("restart"),
        "{}",
        s.systemctl_log()
    );
}

#[test]
fn update_never_prompts() {
    // No key file, no terminal: an install asks about the key, an update must not.
    let s = Setup::new("noprompt");
    let out = s.run(&["--update"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let all = format!("{}{}", text(&out), String::from_utf8_lossy(&out.stderr));
    assert!(!all.to_lowercase().contains("key"), "{all}");
    assert_eq!(s.installed_version(), "soloist 1.0");
}

#[test]
fn update_download_failure_keeps_files() {
    let s = Setup::new("offline");
    assert!(s.run(&[]).status.success());
    let hash = std::fs::read_to_string(s.dest().join(".tarball.sha256")).unwrap();
    s.tarball("2.0");
    s.marker("curl-fails");
    s.marker("active");

    let out = s.run(&["--update"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(s.installed_version(), "soloist 1.0");
    assert_eq!(
        std::fs::read_to_string(s.dest().join(".tarball.sha256")).unwrap(),
        hash
    );
    assert!(!s.systemctl_log().contains("restart"));
}
