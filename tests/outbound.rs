use mad_super_bot::{
    config::Config,
    outbound::{Decision, Queue, RateLimiter, Request, Source},
    twitch::SendOutcome,
};
use std::time::Duration;
use tokio::time::Instant;
fn config() -> Config {
    mad_super_bot::config::LoadedConfig::parse(
        include_str!("fixtures/config.yml"),
        std::path::Path::new("config.yml"),
    )
    .unwrap()
    .runtime
}
#[test]
fn queue_is_bounded_and_resolves_latest_content_and_blacklist() {
    let mut c = config();
    let mut queue = Queue::default();
    let now = Instant::now();
    let request = Request::command("project", "u", "user", now);
    for _ in 0..64 {
        assert!(queue.push(request.clone()));
    }
    assert!(!queue.push(request.clone()));
    c.commands.get_mut("project").unwrap().message = "Changed".into();
    assert_eq!(request.text(&c), Some("Changed"));
    c.blacklist.insert("user".into());
    assert_eq!(request.text(&c), None);
    c.blacklist.clear();
    c.commands.remove("project");
    assert_eq!(request.text(&c), None);
    assert!(request.expired(now + Duration::from_secs(30)));
    let loop_request = Request::loop_job("socials", 7, Some(1), now);
    assert!(matches!(
        loop_request.source,
        Source::Loop {
            revision: 7,
            phase: Some(1),
            ..
        }
    ));
}
#[test]
fn shared_limiter_enforces_twenty_per_thirty_seconds_and_channel_spacing() {
    let start = Instant::now();
    let mut limits = RateLimiter::default();
    for i in 0..20 {
        let time = start + Duration::from_secs(i);
        assert!(limits.ready(time));
        limits.record(time);
        assert!(!limits.ready(time + Duration::from_millis(999)));
    }
    assert!(!limits.ready(start + Duration::from_secs(29)));
    assert!(limits.ready(start + Duration::from_secs(30)));
    limits.block(start + Duration::from_secs(30), Duration::from_secs(10));
    assert!(!limits.ready(start + Duration::from_secs(39)));
    assert!(limits.ready(start + Duration::from_secs(40)));
}
#[test]
fn only_safe_rejections_retry_within_ttl_and_auth_only_once() {
    let now = Instant::now();
    let mut req = Request::command("project", "u", "user", now);
    assert!(matches!(
        req.decision(
            SendOutcome::Unauthorized {
                rejected_token: "dummy".into()
            },
            now
        ),
        Decision::Refresh(_)
    ));
    assert!(matches!(
        req.decision(
            SendOutcome::Unauthorized {
                rejected_token: "dummy".into()
            },
            now
        ),
        Decision::Finish
    ));
    assert!(matches!(
        req.decision(
            SendOutcome::RateLimited {
                retry_after: Duration::from_secs(2)
            },
            now
        ),
        Decision::Retry(_)
    ));
    assert!(matches!(
        req.decision(
            SendOutcome::RateLimited {
                retry_after: Duration::from_secs(30)
            },
            now
        ),
        Decision::Finish
    ));
    for outcome in [
        SendOutcome::Uncertain,
        SendOutcome::Rejected {
            code: "banned".into(),
            cooldown: Duration::ZERO,
        },
        SendOutcome::Sent {
            cooldown: Duration::ZERO,
        },
    ] {
        assert!(matches!(req.decision(outcome, now), Decision::Finish));
    }
}
