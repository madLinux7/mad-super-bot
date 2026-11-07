use std::process::Command;
const YAML: &str = include_str!("../config.example.yml");
fn cli(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mad-super-bot"));
    c.current_dir(dir)
        .env("TWITCH_CLIENT_ID", "environment-sensitive-value")
        .env("TWITCH_CLIENT_SECRET", "environment-sensitive-value")
        .env("TWITCH_CHANNEL", "invalid channel")
        .env("BOT_CONFIG_DIR", "/missing")
        .env("BOT_TOKEN_FILE", "/missing");
    c
}
#[test]
fn help_and_version_need_no_configuration() {
    let dir = tempfile::tempdir().unwrap();
    for arg in ["--help", "--version", "auth --help", "check-config --help"] {
        let o = cli(dir.path()).args(arg.split(' ')).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}
#[test]
fn check_config_is_offline_and_old_environment_and_dotenv_are_inert() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "TWITCH_CLIENT_SECRET=\"sensitive-value\0\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("config.yml"), YAML).unwrap();
    let o = cli(dir.path())
        .env("PATH", "/missing")
        .arg("check-config")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("8 commands, 2 loops"));
    let yml = YAML.replace("bot_credentials: bitwarden", "bot_credentials: yml");
    std::fs::write(dir.path().join("other.yml"), yml).unwrap();
    for args in [
        ["--config", "other.yml", "check-config"],
        ["check-config", "--config", "other.yml"],
    ] {
        let o = cli(dir.path()).args(args).output().unwrap();
        assert!(!o.status.success());
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("other.yml") && err.contains("twitch.client_id"),
            "{err}"
        );
        assert!(!err.contains("sensitive-value"));
    }
}
#[test]
fn errors_are_private_and_removed_options_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.yml"),
        YAML.replace("user_cooldown: 1", "user_cooldown: sensitive-value"),
    )
    .unwrap();
    let o = cli(dir.path()).arg("check-config").output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    assert!(
        err.contains("config.yml") && err.contains("user_cooldown"),
        "{err}"
    );
    assert!(!err.contains("sensitive-value"));
    for arg in [
        "--bitwarden",
        "-bw",
        "--wrapper-config",
        "--config-dir",
        "--token-file",
    ] {
        assert!(!cli(dir.path()).arg(arg).output().unwrap().status.success());
    }
}
#[test]
fn startup_uses_yml_credentials_and_config_relative_token_file() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    std::fs::create_dir(&settings).unwrap();
    let yml = YAML
        .replace("bot_credentials: bitwarden", "bot_credentials: yml")
        .replace("# client_id: deine_client_id", "client_id: dummy-client")
        .replace(
            "# client_secret: dein_client_secret",
            "client_secret: sensitive-value",
        );
    std::fs::write(settings.join("config.yml"), yml).unwrap();
    let o = cli(dir.path())
        .args(["--config", "settings/config.yml"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    assert!(err.contains("token file missing"), "{err}");
    assert!(!err.contains("sensitive-value"));
}
