use mad_super_bot::config::{ConfigPaths, LoadedConfig, LoopMode, LoopPayload};
use std::{path::Path, time::Duration};
const YAML: &str = include_str!("../config.example.yml");
fn parse(text: &str) -> anyhow::Result<LoadedConfig> {
    LoadedConfig::parse(text, Path::new("test.yml"))
}
fn rejected(text: &str, path: &str) {
    let error = match parse(text) {
        Ok(_) => panic!("configuration accepted"),
        Err(e) => format!("{e:#}"),
    };
    assert!(error.contains("test.yml"), "{error}");
    assert!(error.contains(path), "{error}");
    assert!(!error.contains("sensitive-value"), "{error}");
}
#[test]
fn preserves_current_bot_content() {
    let config = parse(YAML).unwrap().runtime;
    assert_eq!(config.commands.len(), 8);
    assert_eq!(config.command_name("InStA"), Some("instagram"));
    assert_eq!(
        config.commands["instagram"].message,
        "https://www.instagram.com/mad__linux"
    );
    assert_eq!(config.user_cooldown, Duration::from_secs(1));
    assert!(config.blacklist.is_empty());
    assert_eq!(config.mode, LoopMode::LiveOnly);
    assert_eq!(config.loops["discord"].interval, Duration::from_secs(1800));
    assert_eq!(
        config.loops["socials"].payload,
        LoopPayload::Command("socials".into())
    );
}
#[test]
fn parses_comma_separated_aliases() {
    let config = parse(&YAML.replace("sns, sm", " sns , sm , network "))
        .unwrap()
        .runtime;
    assert_eq!(config.commands["socials"].alias, ["sns", "sm", "network"]);
    assert_eq!(config.command_name("NeTwOrK"), Some("socials"));
    let config = parse(&YAML.replace("sns, sm", "  ")).unwrap().runtime;
    assert!(config.commands["socials"].alias.is_empty());
    for aliases in ["sns,", ",sm", "sns,,sm", "sns, SNS"] {
        rejected(&YAML.replace("sns, sm", aliases), "commands.socials.alias");
    }
}

#[test]
fn credential_source_is_required_and_never_falls_back() {
    rejected(
        &YAML.replace("bot_credentials: bitwarden", ""),
        "bot_credentials",
    );
    rejected(
        &YAML.replace("bot_credentials: bitwarden", "bot_credentials: automatic"),
        "bot_credentials",
    );
    rejected(
        &YAML.replace("bot_credentials: bitwarden", "bot_credentials: yml"),
        "twitch.client_id",
    );
    let yml = YAML
        .replace("bot_credentials: bitwarden", "bot_credentials: yml")
        .replace(
            "  # client_id: deine_client_id",
            "  client_id: sensitive-value",
        )
        .replace(
            "  # client_secret: dein_client_secret",
            "  client_secret: sensitive-value",
        );
    assert!(parse(&yml).is_ok());
    rejected(
        &yml.replace("  client_secret: sensitive-value", "  client_secret: ''"),
        "twitch.client_secret",
    );
    rejected(
        &YAML.replace("  item_id: deine_item_id", "  item_id: ''"),
        "bitwarden.item_id",
    );
}
#[test]
fn rejects_unknown_duplicate_missing_and_mistyped_fields_without_values() {
    for (text, path) in [
        (
            YAML.replace("user_cooldown: 1", "user_cooldown: sensitive-value"),
            "user_cooldown",
        ),
        (
            YAML.replace("user_cooldown: 1", "user_cooldown: -1"),
            "user_cooldown",
        ),
        (
            YAML.replace("user_cooldown: 1", "user_cooldown: 0.5"),
            "user_cooldown",
        ),
        (YAML.replace("user_cooldown: 1", ""), "user_cooldown"),
        (
            YAML.replace("mode: \"live-only\"", "mode: sensitive-value"),
            "mode",
        ),
        (YAML.replace("prefix: \"!\"", "prefix: ' '"), "prefix"),
        (YAML.to_owned() + "\nunknown: sensitive-value", "unknown"),
        (YAML.to_owned() + "\nprefix: sensitive-value", "prefix"),
        (
            YAML.replace(
                "  channel: dein_channel",
                "  channel: dein_channel\n  typo: sensitive-value",
            ),
            "twitch",
        ),
        (
            YAML.replace("    interval: 30", "    interval: sensitive-value"),
            "loops.discord.interval",
        ),
        (
            YAML.replace("    interval: 30", "    interval: 30\n    interval: 60"),
            "loops.discord.interval",
        ),
        (
            YAML.replace(
                "    alias: \"projekt\"",
                "    alias: \"projekt\"\n    typo: sensitive-value",
            ),
            "commands.project",
        ),
        (
            YAML.replace("    alias: \"projekt\"", "    alias: \"sns\""),
            "commands",
        ),
    ] {
        rejected(&text, path);
    }
}
#[test]
fn validates_intervals_payloads_names_and_messages() {
    for value in ["0", "-1", ".nan", ".inf", "1e300", "1e-300", "'30'"] {
        rejected(
            &YAML.replace("    interval: 30", &format!("    interval: {value}")),
            "loops.discord.interval",
        );
    }
    let config = parse(&YAML.replace("    interval: 30", "    interval: 0.5")).unwrap();
    assert_eq!(
        config.runtime.loops["discord"].interval,
        Duration::from_secs(30)
    );
    for command in ["missing", "sns"] {
        rejected(
            &YAML.replace("command: \"socials\"", &format!("command: {command}")),
            "loops.socials.command",
        );
    }
    rejected(
        &YAML.replace(
            "command: \"socials\"",
            "command: socials\n    message: sensitive-value",
        ),
        "loops.socials",
    );
    for text in ["", " ", &"x".repeat(501)] {
        rejected(
            &YAML.replace("message: \"Der Geilste\"", &format!("message: '{text}'")),
            "commands.johann.message",
        );
    }
    for alias in ["!bad", "two words", "ä"] {
        rejected(
            &YAML.replace("\"projekt\"", &format!("'{alias}'")),
            "commands.project.alias",
        );
    }
    assert!(parse(&YAML.replace("user_cooldown: 1", "user_cooldown: 0")).is_ok());
}
#[test]
fn relative_token_paths_are_literal_and_relative_to_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.yml");
    std::fs::write(
        &file,
        YAML.replace(
            "token_file: tokens.json",
            "token_file: '$HOME/${TOKEN}/~/tokens.json'",
        ),
    )
    .unwrap();
    let paths = ConfigPaths::new(file).unwrap();
    assert_eq!(
        paths.load().unwrap().startup.token_file,
        dir.path().join("$HOME/${TOKEN}/~/tokens.json")
    );
}
