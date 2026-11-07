use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
};
use tempfile::TempDir;
const BW: &str = r#"#!/usr/bin/python3
import os,sys,json,time,shutil
cmd=sys.argv[1]; fresh='dummy-fresh-session'
mode=os.environ.get('CASE','')
assert not any(k.startswith('BW_') for k in os.environ if k!='BW_SESSION')
expected=fresh if cmd in ['sync','get','lock'] else None
if cmd=='lock' and mode in ['fail-unlock','pause-unlock']: expected=None
assert os.environ.get('BW_SESSION') == expected
assert os.environ.get('BITWARDENCLI_APPDATA_DIR') == 'dummy-profile'
assert fresh not in sys.argv
with open('events','a') as f: f.write(cmd+'\n')
mode=os.environ.get('CASE','')
if mode=='hang-get' and cmd=='get': time.sleep(60)
if mode=='pause-'+cmd:
    time.sleep(.25 if cmd=='lock' else 30)
if mode=='fail-'+cmd:
    print('dummy-sensitive-cli-error',file=sys.stderr);sys.exit(1)
if cmd=='lock':
    # An old re-exec implementation would run this replacement, which must never happen.
    shutil.copy('bot','next-bot'); os.replace('next-bot','mad-super-bot')
if cmd=='status': print(json.dumps({'status':'unauthenticated' if mode=='logged-out' else 'locked'}))
if cmd=='unlock':
    assert sys.argv[2:]==['--raw']; assert sys.stdin.isatty(); print(fresh)
if cmd=='get':
    assert sys.argv[2:]==['item','changed-item' if mode=='changed-config' else 'configured-item']
    fields=[{'name':'custom-id','value':'dummy-id;$(touch SHELL_EXECUTED)'},{'name':'custom-secret','value':'dummy-secret\"\n$HOME'}]
    if mode=='changed-config': fields[0]['name']='renamed-id';fields[1]['name']='renamed-secret'
    if mode=='missing': fields.pop()
    if mode=='duplicate': fields.append(fields[1])
    if mode=='empty': fields[1]['value']=''
    if mode=='non-string': fields[1]['value']=17
    print('invalid-json' if mode=='invalid-json' else json.dumps({'fields':fields}))
"#;
const DRIVER: &str = r#"
import os,sys,pty,subprocess,time,signal,fcntl,termios
master,slave=pty.openpty()
def setup():
 os.setsid(); fcntl.ioctl(slave,termios.TIOCSCTTY,0)
p=subprocess.Popen(sys.argv[1:],stdin=slave,stdout=subprocess.PIPE,stderr=subprocess.PIPE,preexec_fn=setup)
os.close(slave)
mode=os.environ.get('CASE','')
if mode.startswith('pause-'):
 target=mode[6:]
 for i in range(500):
  if os.path.exists('events') and target in open('events').read().splitlines(): break
  time.sleep(.01)
 p.send_signal(signal.SIGTERM if target=='get' else signal.SIGINT)
try: out,err=p.communicate(timeout=40 if mode=='hang-get' else 10)
except subprocess.TimeoutExpired:
 os.killpg(p.pid,signal.SIGKILL);out,err=p.communicate();sys.stderr.buffer.write(err);raise
sys.stdout.buffer.write(out);sys.stderr.buffer.write(err)
if p.returncode<0: print('signal:'+str(-p.returncode));sys.exit(128-p.returncode)
sys.exit(p.returncode)
"#;
struct Fixture(TempDir);
impl Fixture {
    fn new() -> Self {
        let d = tempfile::tempdir().unwrap();
        for (name, script) in [
            ("bw", BW),
            ("bot", "#!/bin/sh\necho UNEXPECTED_REEXEC\nexit 23\n"),
        ] {
            let p = d.path().join(name);
            fs::write(&p, script).unwrap();
            fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let yaml = include_str!("../config.example.yml")
            .replace("deine_item_id", "configured-item")
            .replace("client_id_field: client_id", "client_id_field: custom-id")
            .replace(
                "client_secret_field: client_secret",
                "client_secret_field: custom-secret",
            );
        fs::write(d.path().join("config.yml"), yaml).unwrap();
        Self(d)
    }
    fn run(&self, mode: &str, tty: bool) -> Output {
        let binary = self.0.path().join("mad-super-bot");
        let pending = self.0.path().join("pending-binary");
        if mode == "pause-after-lock" {
            fs::copy(std::env::current_exe().unwrap(), &pending).unwrap();
        } else {
            fs::copy(env!("CARGO_BIN_EXE_mad-super-bot"), &pending).unwrap();
        }
        fs::rename(pending, &binary).unwrap();
        let mut c = if tty {
            let mut c = Command::new("/usr/bin/python3");
            c.args(["-c", DRIVER, binary.to_str().unwrap()]);
            c
        } else {
            Command::new(&binary)
        };
        c.current_dir(self.0.path())
            .env("PATH", self.0.path())
            .env("CASE", mode)
            .env("BW_SESSION", "inherited-session")
            .env("BW_PASSWORD", "inherited-password")
            .env("BW_OTHER", "ignored")
            .env("BITWARDENCLI_APPDATA_DIR", "dummy-profile")
            .env("TWITCH_CLIENT_ID", "old")
            .env("TWITCH_CLIENT_SECRET", "old");
        if mode == "pause-after-lock" {
            c.args(["--exact", "credential_handoff_probe", "--nocapture"]);
        }
        c.output().unwrap()
    }
    fn events(&self) -> Vec<String> {
        fs::read_to_string(self.0.path().join("events"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    fn safe(&self, o: &Output) {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        for secret in [
            "dummy-fresh-session",
            "dummy-secret",
            "dummy-id",
            "dummy-sensitive-cli-error",
            "inherited-session",
            "inherited-password",
            "UNEXPECTED_REEXEC",
        ] {
            assert!(!text.contains(secret), "sensitive output or re-exec");
        }
        assert!(!self.0.path().join("SHELL_EXECUTED").exists());
    }
}
#[test]
fn credentials_return_in_same_process_only_after_lock() {
    let f = Fixture::new();
    let o = f.run("", true);
    f.safe(&o);
    assert_eq!(f.events(), ["status", "unlock", "sync", "get", "lock"]);
    // Startup reached the token check in the original process after the executable was replaced.
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("token file missing"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
#[test]
fn malformed_fields_and_cli_failures_lock_without_using_credentials() {
    for mode in [
        "missing",
        "duplicate",
        "empty",
        "non-string",
        "invalid-json",
        "fail-unlock",
        "fail-sync",
        "fail-get",
        "fail-lock",
    ] {
        let f = Fixture::new();
        let o = f.run(mode, true);
        f.safe(&o);
        assert!(!o.status.success(), "{mode}");
        assert!(f.events().contains(&"lock".into()), "{mode}");
        assert!(
            !String::from_utf8_lossy(&o.stderr).contains("token file missing"),
            "{mode}"
        );
    }
}
#[test]
fn invalid_configuration_fails_before_cli() {
    for config in ["", "bot_credentials: bitwarden", "twitch: [broken"] {
        let f = Fixture::new();
        fs::write(f.0.path().join("config.yml"), config).unwrap();
        let o = f.run("", true);
        f.safe(&o);
        assert!(!o.status.success());
        assert!(f.events().is_empty());
    }
}
#[test]
fn prerequisites_fail_before_unlock() {
    for mode in ["no-tty", "logged-out", "missing-bw", "missing-config"] {
        let f = Fixture::new();
        if let Some(name) = mode.strip_prefix("missing-") {
            fs::remove_file(
                f.0.path()
                    .join(if name == "config" { "config.yml" } else { name }),
            )
            .unwrap();
        }
        let o = f.run(mode, mode != "no-tty");
        f.safe(&o);
        assert!(!o.status.success());
        assert!(!f.events().contains(&"unlock".into()), "{mode}");
    }
}
#[test]
fn interrupted_commands_are_reaped_then_locked() {
    for mode in ["pause-unlock", "pause-get", "pause-lock"] {
        let f = Fixture::new();
        let o = f.run(mode, true);
        f.safe(&o);
        assert!(!o.status.success());
        assert_eq!(
            f.events().last().map(String::as_str),
            Some("lock"),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(!String::from_utf8_lossy(&o.stderr).contains("token file missing"));
    }
}
#[test]
fn changed_bitwarden_reference_is_used_on_next_start() {
    let f = Fixture::new();
    let p = f.0.path().join("config.yml");
    let text = fs::read_to_string(&p)
        .unwrap()
        .replace("configured-item", "changed-item")
        .replace("custom-id", "renamed-id")
        .replace("custom-secret", "renamed-secret");
    fs::write(p, text).unwrap();
    let o = f.run("changed-config", true);
    f.safe(&o);
    assert_eq!(f.events(), ["status", "unlock", "sync", "get", "lock"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("token file missing"));
}
#[test]
fn stalled_noninteractive_call_times_out_and_locks_without_a_signal() {
    let f = Fixture::new();
    let o = f.run("hang-get", true);
    f.safe(&o);
    assert!(!o.status.success());
    assert_eq!(f.events().last().map(String::as_str), Some("lock"));
    assert!(String::from_utf8_lossy(&o.stderr).contains("Zeitlimit"));
}

#[test]
fn credential_handoff_probe() {
    if std::env::var("CASE").as_deref() != Ok("pause-after-lock") {
        return;
    }
    mad_super_bot::startup::run(async {
        let mut shutdown = mad_super_bot::startup::Shutdown::new()?;
        let config = mad_super_bot::config::ConfigPaths::new("config.yml")?.load()?;
        let _credentials = config.startup.credentials().await?;
        use std::io::Write;
        let mut events = fs::OpenOptions::new().append(true).open("events")?;
        writeln!(events, "after-lock")?;
        let result: anyhow::Result<()> = shutdown.until(std::future::pending()).await;
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        Ok(())
    })
    .unwrap();
}
#[test]
fn shutdown_after_vault_lock_cancels_authorization_wait() {
    let f = Fixture::new();
    let output = f.run("pause-after-lock", true);
    f.safe(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        f.events(),
        ["status", "unlock", "sync", "get", "lock", "after-lock"]
    );
}
