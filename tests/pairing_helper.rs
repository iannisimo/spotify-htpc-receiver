//! `packaging/pairing-port` (the root-side pairing helper) run as a normal user against fake
//! `ss`, `ps`, `ip`, `ufw`, `firewall-cmd` and `nft`. The fakes log their arguments to
//! `<dir>/log`; `ufw` and `firewall-cmd` keep their rules in files, so deleting a ufw rule
//! renumbers the rest as the real one does.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SS_SOLOIST: &str = "\
LISTEN 0      4096       127.0.0.1:9090       0.0.0.0:*    users:((\"soloist\",pid=4242,fd=26))
LISTEN 0      12           0.0.0.0:51117      0.0.0.0:*    users:((\"soloist\",pid=4242,fd=27))
LISTEN 0      128          0.0.0.0:22         0.0.0.0:*    users:((\"sshd\",pid=1,fd=3))
";
const IP_DEFAULT: &str = "default via 10.0.0.1 dev wlp194s0 proto dhcp src 10.0.0.133 metric 600\n";
const IP_DEV: &str = "10.0.0.0/24 proto kernel scope link src 10.0.0.133 metric 600\n";
const OWN_RULE: &str = "51117/tcp|10.0.0.0/24|htpc-nowplaying-pairing";
const RICH_RULE: &str = "rule family=\"ipv4\" source address=\"10.0.0.0/24\" port port=\"51117\" protocol=\"tcp\" accept";

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

struct Env {
    dir: PathBuf,
}

impl Env {
    /// Fakes for a machine with an active ufw and this user's Soloist listening on 51117.
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("htpc-pairing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("tmp")).unwrap();
        let env = Env { dir };
        let d = env.dir.display();

        env.fake("ss", &format!("cat '{d}/ss.out'\n"));
        env.fake(
            "ps",
            &format!("echo \"ps $*\" >> '{d}/log'\ncat '{d}/ps.out'\n"),
        );
        env.fake(
            "ip",
            &format!(
                "echo \"ip $*\" >> '{d}/log'\n\
                 case \"$*\" in\n\
                 '-4 -o route show default') cat '{d}/ip-default.out' ;;\n\
                 '-4 -o route show dev '*' scope link proto kernel') cat '{d}/ip-dev.out' ;;\n\
                 *) exit 1 ;;\n\
                 esac\n"
            ),
        );
        // Rules are lines of `to|from|comment`. Arguments as the helper must send them:
        // allow from S to any port P proto tcp comment C / --force delete N / status [numbered]
        env.fake(
            "ufw",
            &format!(
                "echo \"ufw $*\" >> '{d}/log'\n\
                 R='{d}/ufw.rules'\n\
                 case \"$1\" in\n\
                 status)\n\
                   if [ ! -e '{d}/ufw.active' ]; then echo 'Status: inactive'; exit 0; fi\n\
                   echo 'Status: active'\n\
                   [ \"${{2:-}}\" = numbered ] || exit 0\n\
                   echo; echo '     To                         Action      From'\n\
                   echo '     --                         ------      ----'\n\
                   n=0\n\
                   while IFS='|' read -r to from comment; do\n\
                     n=$((n+1))\n\
                     line=$(printf '[%2d] %-26s ALLOW IN    %s' \"$n\" \"$to\" \"$from\")\n\
                     [ -n \"$comment\" ] && line=\"$line                # $comment\"\n\
                     echo \"$line\"\n\
                   done < \"$R\" ;;\n\
                 allow) echo \"$7/tcp|$3|${{11}}\" >> \"$R\"; echo 'Rule added' ;;\n\
                 --force) sed -i \"${{3}}d\" \"$R\"; echo 'Rule deleted' ;;\n\
                 *) exit 1 ;;\n\
                 esac\n"
            ),
        );
        env.fake(
            "firewall-cmd",
            &format!(
                "echo \"firewall-cmd $*\" >> '{d}/log'\n\
                 R='{d}/fwd.rules'\n\
                 case \"$1\" in\n\
                 --state) if [ -e '{d}/fwd.running' ]; then echo running; else echo 'not running'; exit 252; fi ;;\n\
                 --add-rich-rule=*) printf '%s\\n' \"${{1#--add-rich-rule=}}\" >> \"$R\"; echo success ;;\n\
                 --remove-rich-rule=*) grep -vxF -- \"${{1#--remove-rich-rule=}}\" \"$R\" > \"$R.tmp\"; mv \"$R.tmp\" \"$R\"; echo success ;;\n\
                 *) exit 1 ;;\n\
                 esac\n"
            ),
        );
        env.fake(
            "nft",
            &format!("[ -e '{d}/nft.out' ] && cat '{d}/nft.out'\nexit 0\n"),
        );

        env.write("ss.out", SS_SOLOIST);
        env.write("ps.out", &format!("  {}\n", uid()));
        env.write("ip-default.out", IP_DEFAULT);
        env.write("ip-dev.out", IP_DEV);
        env.write("ufw.rules", "");
        env.write("fwd.rules", "");
        env.write("log", "");
        env.write("ufw.active", "");
        env
    }

    fn fake(&self, name: &str, body: &str) {
        let path = self.dir.join("bin").join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.dir.join(name), text).unwrap();
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.dir.join(name));
    }

    fn log(&self) -> Vec<String> {
        self.read("log").lines().map(str::to_owned).collect()
    }

    fn run_dir(&self) -> PathBuf {
        self.dir.join("tmp/htpc-nowplaying")
    }

    fn command(&self, args: &[&str]) -> Command {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/pairing-port");
        let mut cmd = Command::new(script);
        cmd.args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.join("bin").display()),
            )
            .env("SUDO_UID", uid())
            .env("TMPDIR", self.dir.join("tmp"))
            .env("PAIRING_PORT_WAIT", "0")
            .env("LC_ALL", "C")
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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn allows(env: &Env) -> Vec<String> {
    env.log()
        .into_iter()
        .filter(|l| l.starts_with("ufw allow") || l.contains("--add-rich-rule"))
        .collect()
}

#[test]
fn ufw_open_allows_soloist_port_from_subnet() {
    let env = Env::new("ufw-open");
    let out = env.run(&["open"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), "backend=ufw state=open port=51117");
    assert_eq!(
        allows(&env),
        ["ufw allow from 10.0.0.0/24 to any port 51117 proto tcp comment htpc-nowplaying-pairing"]
    );
    assert_eq!(env.read("ufw.rules").trim(), OWN_RULE);
}

#[test]
fn open_waits_for_soloist_to_listen() {
    // At Soloist start the unit's ExecStartPost runs before Soloist listens: the first two
    // looks find nothing.
    let env = Env::new("wait");
    let d = env.dir.display();
    env.fake(
        "ss",
        &format!(
            "n=$(cat '{d}/ss.count' 2>/dev/null || echo 0)\n\
             echo $((n+1)) > '{d}/ss.count'\n\
             [ \"$n\" -ge 2 ] && cat '{d}/ss.out'\n\
             exit 0\n"
        ),
    );
    let out = env
        .command(&["open"])
        .env("PAIRING_PORT_WAIT", "5")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), "backend=ufw state=open port=51117");
}

#[test]
fn open_refuses_other_users_soloist() {
    let env = Env::new("other-user");
    env.write("ps.out", "  64123\n");
    let out = env.run(&["open"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(allows(&env).is_empty());
}

#[test]
fn open_ignores_loopback_listener() {
    let env = Env::new("loopback");
    env.write(
        "ss.out",
        "LISTEN 0 4096 127.0.0.1:9090 0.0.0.0:* users:((\"soloist\",pid=4242,fd=26))\n",
    );
    let out = env.run(&["open"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(allows(&env).is_empty());
}

#[test]
fn open_ignores_other_programs() {
    let env = Env::new("not-soloist");
    env.write(
        "ss.out",
        "LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))\n\
         LISTEN 0 128 0.0.0.0:51117 0.0.0.0:* users:((\"soloistx\",pid=7,fd=3))\n",
    );
    assert_eq!(env.run(&["open"]).status.code(), Some(3));
    assert!(allows(&env).is_empty());
}

#[test]
fn close_deletes_only_own_rules_highest_first() {
    let env = Env::new("close");
    env.write(
        "ufw.rules",
        "22/tcp|Anywhere|\n\
         40000/tcp|10.0.0.0/24|htpc-nowplaying-pairing\n\
         8080/tcp|Anywhere|mine\n\
         1234/tcp|Anywhere|htpc-nowplaying-pairing-old\n\
         51117/tcp|10.0.0.0/24|htpc-nowplaying-pairing\n",
    );
    let out = env.run(&["close"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), "backend=ufw state=closed port=-");
    let deletes: Vec<String> = env
        .log()
        .into_iter()
        .filter(|l| l.contains("delete"))
        .collect();
    assert_eq!(deletes, ["ufw --force delete 5", "ufw --force delete 2"]);
    assert_eq!(
        env.read("ufw.rules"),
        "22/tcp|Anywhere|\n8080/tcp|Anywhere|mine\n1234/tcp|Anywhere|htpc-nowplaying-pairing-old\n"
    );
}

#[test]
fn reopen_closes_old_rule_first() {
    let env = Env::new("reopen");
    assert_eq!(env.run(&["open"]).status.code(), Some(0));
    env.write("ss.out", &SS_SOLOIST.replace("51117", "52000"));
    let out = env.run(&["open"]);
    assert_eq!(stdout(&out), "backend=ufw state=open port=52000");
    assert_eq!(
        env.read("ufw.rules").trim(),
        OWN_RULE.replace("51117", "52000")
    );
    let log = env.log();
    let delete = log.iter().position(|l| l.contains("delete")).unwrap();
    let second_allow = log
        .iter()
        .rposition(|l| l.starts_with("ufw allow"))
        .unwrap();
    assert!(delete < second_allow, "{log:?}");
}

#[test]
fn status_reports_open_and_closed() {
    let env = Env::new("status");
    assert_eq!(
        stdout(&env.run(&["status"])),
        "backend=ufw state=closed port=-"
    );
    env.run(&["open"]);
    let out = env.run(&["status"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "backend=ufw state=open port=51117");
    env.run(&["close"]);
    assert_eq!(
        stdout(&env.run(&["status"])),
        "backend=ufw state=closed port=-"
    );
}

#[test]
fn firewalld_open_close_round_trip() {
    let env = Env::new("firewalld");
    env.remove("ufw.active");
    env.write("fwd.running", "");

    let out = env.run(&["open"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), "backend=firewalld state=open port=51117");
    assert_eq!(env.read("fwd.rules").trim(), RICH_RULE);
    assert!(
        !env.log().iter().any(|l| l.contains("--permanent")),
        "runtime rules only"
    );
    let tracked = env.run_dir().join("firewalld-rules");
    assert_eq!(std::fs::read_to_string(&tracked).unwrap().trim(), RICH_RULE);
    assert_eq!(
        stdout(&env.run(&["status"])),
        "backend=firewalld state=open port=51117"
    );

    let out = env.run(&["close"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), "backend=firewalld state=closed port=-");
    assert_eq!(env.read("fwd.rules"), "");
    assert!(!tracked.exists());
    assert!(
        env.log()
            .iter()
            .any(|l| l == &format!("firewall-cmd --remove-rich-rule={RICH_RULE}"))
    );
}

#[test]
fn no_firewall_is_ok() {
    let env = Env::new("none");
    env.remove("ufw.active");
    let out = env.run(&["open"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), "backend=none state=closed port=-");
    assert!(allows(&env).is_empty());
    assert_eq!(env.run(&["close"]).status.code(), Some(0));
}

#[test]
fn unsupported_firewall_reports_the_port() {
    let env = Env::new("unsupported");
    env.remove("ufw.active");
    env.write(
        "nft.out",
        "table inet filter {\n\tchain input {\n\t\ttype filter hook input priority filter; policy drop;\n\t}\n}\n",
    );
    let out = env.run(&["open"]);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert_eq!(stdout(&out), "backend=unsupported state=closed port=51117");
    assert!(allows(&env).is_empty());
    // Nothing of ours to remove, so close succeeds.
    assert_eq!(env.run(&["close"]).status.code(), Some(0));
}

#[test]
fn refuses_a_subnet_wider_than_a_local_network() {
    let env = Env::new("wide");
    env.write(
        "ip-dev.out",
        "0.0.0.0/0 proto kernel scope link src 10.0.0.133\n",
    );
    let out = env.run(&["open"]);
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    assert!(allows(&env).is_empty());
}

#[test]
fn opens_every_local_subnet_of_the_interface() {
    let env = Env::new("two-subnets");
    env.write(
        "ip-dev.out",
        "10.0.0.0/24 proto kernel scope link src 10.0.0.133\n\
         192.168.5.0/24 proto kernel scope link src 192.168.5.2\n",
    );
    assert_eq!(env.run(&["open"]).status.code(), Some(0));
    assert_eq!(allows(&env).len(), 2);
    env.run(&["close"]);
    assert_eq!(env.read("ufw.rules"), "");
}

#[test]
fn duplicate_link_routes_give_one_rule() {
    // Seen on the dev laptop: the same subnet once without and once with a metric.
    let env = Env::new("dup-routes");
    env.write(
        "ip-dev.out",
        "10.0.0.0/24 src 10.0.0.133 \n10.0.0.0/24 src 10.0.0.133 metric 600\n",
    );
    assert_eq!(env.run(&["open"]).status.code(), Some(0));
    assert_eq!(allows(&env).len(), 1);
}

#[test]
fn refuses_without_sudo_uid() {
    let env = Env::new("no-sudo-uid");
    for args in [["open"], ["close"], ["status"]] {
        let out = env.command(&args).env_remove("SUDO_UID").output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    assert!(env.log().is_empty(), "nothing ran");
}

#[test]
fn refuses_a_malformed_sudo_uid() {
    let env = Env::new("bad-sudo-uid");
    let out = env
        .command(&["open"])
        .env("SUDO_UID", "1000 0")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(env.log().is_empty());
}

#[test]
fn refuses_extra_arguments() {
    let env = Env::new("args");
    for args in [&["open", "1234"][..], &[], &["frobnicate"], &["--help"]] {
        assert_eq!(env.run(args).status.code(), Some(2), "{args:?}");
    }
    assert!(env.log().is_empty(), "nothing ran");
}
