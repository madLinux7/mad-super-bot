use mad_super_bot::{
    config::Config,
    live_status::{LiveState, Status},
    scheduler::Scheduler,
};
use std::time::Duration;
use tokio::time::Instant;
fn config(mode: &str) -> Config {
    let text = include_str!("../config.example.yml")
        .split("# live-only or always-on")
        .next()
        .unwrap();
    mad_super_bot::config::LoadedConfig::parse(
        &format!(
            "{text}\nmode: {mode}\nloops:\n  hint:\n    interval: 0.5\n    command: socials\n"
        ),
        std::path::Path::new("config.yml"),
    )
    .unwrap()
    .runtime
}
#[tokio::test(start_paused = true)]
async fn live_only_waits_full_interval_and_never_catches_up() {
    let c = config("live-only");
    let mut live = LiveState::default();
    let mut scheduler = Scheduler::default();
    let now = Instant::now();
    scheduler.reconcile(&c, &live, true, now);
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(scheduler.due(Instant::now()).is_empty());
    live.event(Status::Live);
    scheduler.reconcile(&c, &live, true, Instant::now());
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(scheduler.due(Instant::now()).is_empty());
    tokio::time::advance(Duration::from_secs(1)).await;
    let job = scheduler.due(Instant::now()).pop().unwrap();
    assert!(scheduler.valid(&job.source));
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert!(scheduler.due(Instant::now()).is_empty());
    scheduler.complete(&job.source, Instant::now());
    assert!(scheduler.due(Instant::now()).is_empty());
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(scheduler.due(Instant::now()).len(), 1);
}
#[tokio::test(start_paused = true)]
async fn offline_disconnect_and_new_phase_invalidate_pending_work() {
    let c = config("live-only");
    let mut live = LiveState::default();
    live.event(Status::Live);
    let mut scheduler = Scheduler::default();
    scheduler.reconcile(&c, &live, true, Instant::now());
    tokio::time::advance(Duration::from_secs(30)).await;
    let old = scheduler.due(Instant::now()).pop().unwrap();
    for status in [Status::Offline, Status::Unknown] {
        live.event(status);
        scheduler.reconcile(&c, &live, true, Instant::now());
        assert!(!scheduler.valid(&old.source));
    }
    live.event(Status::Live);
    scheduler.reconcile(&c, &live, true, Instant::now());
    scheduler.complete(&old.source, Instant::now());
    assert!(scheduler.due(Instant::now()).is_empty());
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(scheduler.due(Instant::now()).len(), 1);
}
#[tokio::test(start_paused = true)]
async fn always_on_works_offline_text_preserves_deadlines_and_interval_mode_reset() {
    let mut c = config("always-on");
    let mut live = LiveState::default();
    let mut s = Scheduler::default();
    s.reconcile(&c, &live, false, Instant::now());
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(s.due(Instant::now()).is_empty());
    live.event(Status::Offline);
    s.reconcile(&c, &live, true, Instant::now());
    tokio::time::advance(Duration::from_secs(20)).await;
    c.commands.get_mut("socials").unwrap().message = "new".into();
    s.reconcile(&c, &live, true, Instant::now());
    tokio::time::advance(Duration::from_secs(10)).await;
    let job = s.due(Instant::now()).pop().unwrap();
    assert_eq!(job.text(&c), Some("new"));
    s.complete(&job.source, Instant::now());
    tokio::time::advance(Duration::from_secs(20)).await;
    c.loops.get_mut("hint").unwrap().interval = Duration::from_secs(60);
    s.reconcile(&c, &live, true, Instant::now());
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(s.due(Instant::now()).is_empty());
    c.mode = mad_super_bot::config::LoopMode::LiveOnly;
    live.event(Status::Live);
    s.reconcile(&c, &live, true, Instant::now());
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(s.due(Instant::now()).is_empty());
    tokio::time::advance(Duration::from_secs(1)).await;
    let job = s.due(Instant::now()).pop().unwrap();
    c.loops.clear();
    s.reconcile(&c, &live, true, Instant::now());
    assert!(!s.valid(&job.source));
}
