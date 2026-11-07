use mad_super_bot::{
    config::{ConfigPaths, LoadedConfig},
    reload::Reload,
};
use std::time::Duration;
const YAML: &str = include_str!("fixtures/config.yml");
async fn next(reload: &mut Reload) -> anyhow::Result<LoadedConfig> {
    tokio::time::timeout(Duration::from_secs(3), reload.next())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn valid_syntax_error_repair_atomic_replace_and_startup_changes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.yml");
    std::fs::write(&file, YAML).unwrap();
    let paths = ConfigPaths::new(&file).unwrap();
    let initial = paths.load().unwrap();
    let mut reload = Reload::start(paths).unwrap();
    let changed = YAML
        .replace("madLink", "changed")
        .replace("user_cooldown: 1", "user_cooldown: 5")
        .replace("blacklist: []", "blacklist: [Blocked]")
        .replace("mode: \"live-only\"", "mode: always-on")
        .replace("interval: 30", "interval: 15");
    std::fs::write(&file, &changed).unwrap();
    let snapshot = next(&mut reload).await.unwrap();
    assert!(
        snapshot.runtime.commands["project"]
            .message
            .contains("changed")
    );
    assert_eq!(snapshot.runtime.user_cooldown, Duration::from_secs(5));
    assert!(snapshot.runtime.blacklist.contains("blocked"));
    assert_eq!(
        snapshot.runtime.loops["discord"].interval,
        Duration::from_secs(900)
    );
    assert!(snapshot.startup == initial.startup);
    std::fs::write(&file, "twitch: [broken").unwrap();
    assert!(next(&mut reload).await.is_err());
    std::fs::write(&file, &changed).unwrap();
    assert!(next(&mut reload).await.is_ok());
    for (from, to) in [
        ("channel: dein_channel", "channel: another"),
        ("token_file: tokens.json", "token_file: other.json"),
        ("item_id: deine_item_id", "item_id: changed"),
    ] {
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, changed.replace(from, to)).unwrap();
        std::fs::rename(&replacement, &file).unwrap();
        assert!(next(&mut reload).await.unwrap().startup != initial.startup);
    }
    std::fs::write(dir.path().join("unrelated.yml"), "broken").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(400), reload.next())
            .await
            .is_err()
    );
}
