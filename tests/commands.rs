use mad_super_bot::{
    commands::{Chat, Commands},
    config::Config,
    outbound::Queue,
};
use std::time::Duration;
use tokio::time::Instant;
fn config() -> Config {
    let mut config = mad_super_bot::config::LoadedConfig::parse(
        include_str!("fixtures/config.yml"),
        std::path::Path::new("config.yml"),
    )
    .unwrap()
    .runtime;
    config.user_cooldown = Duration::from_secs(17);
    config.blacklist.insert("spammyuser187".into());
    config
}
fn call(
    state: &mut Commands,
    queue: &mut Queue,
    c: &Config,
    id: &str,
    login: &str,
    text: &str,
    now: Instant,
) -> bool {
    state.accept(
        c,
        "bot",
        Chat {
            user_id: id,
            login,
            text,
        },
        now,
        queue,
    )
}
#[test]
fn cooldown_is_shared_by_alias_and_command_but_not_users() {
    let (mut state, mut queue, c, now) = (
        Commands::default(),
        Queue::default(),
        config(),
        Instant::now(),
    );
    assert!(call(
        &mut state, &mut queue, &c, "one", "one", "!insta", now
    ));
    assert!(!call(
        &mut state,
        &mut queue,
        &c,
        "one",
        "one",
        "!project",
        now + Duration::from_secs(16)
    ));
    assert!(call(
        &mut state,
        &mut queue,
        &c,
        "two",
        "two",
        "!project",
        now + Duration::from_secs(16)
    ));
    assert!(call(
        &mut state,
        &mut queue,
        &c,
        "one",
        "one",
        "!project",
        now + Duration::from_secs(17)
    ));
}
#[test]
fn matching_is_exact_ascii_and_silent_for_blacklist_and_bot() {
    let mut c = config();
    c.user_cooldown = Duration::ZERO;
    let (mut state, mut queue, now) = (Commands::default(), Queue::default(), Instant::now());
    for text in ["  !InStA additional words", "\t!PROJECT", "!sns"] {
        assert!(call(&mut state, &mut queue, &c, "one", "one", text, now));
    }
    for (id, login, text) in [
        ("bot", "normal", "!project"),
        ("one", "SpAmMyUsEr187", "!project"),
        ("one", "one", "!instagrammore"),
        ("one", "one", "hello !project"),
        ("one", "one", "!missing"),
    ] {
        assert!(!call(&mut state, &mut queue, &c, id, login, text, now));
    }
    assert_eq!(queue.len(), 3);
}
#[test]
fn full_queue_does_not_start_cooldown_and_reload_uses_existing_timestamps() {
    let (mut state, mut queue, mut c, now) = (
        Commands::default(),
        Queue::default(),
        config(),
        Instant::now(),
    );
    for i in 0..64 {
        assert!(call(
            &mut state,
            &mut queue,
            &c,
            &format!("u{i}"),
            "user",
            "!project",
            now
        ));
    }
    assert!(!call(
        &mut state, &mut queue, &c, "overflow", "user", "!project", now
    ));
    queue.pop();
    assert!(call(
        &mut state, &mut queue, &c, "overflow", "user", "!project", now
    ));
    queue.pop();
    c.user_cooldown = Duration::from_secs(30);
    assert!(!call(
        &mut state,
        &mut queue,
        &c,
        "overflow",
        "user",
        "!project",
        now + Duration::from_secs(20)
    ));
    assert!(call(
        &mut state,
        &mut queue,
        &c,
        "overflow",
        "user",
        "!project",
        now + Duration::from_secs(30)
    ));
}
