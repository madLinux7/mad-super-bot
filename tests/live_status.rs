use mad_super_bot::live_status::{LiveState, Status};
#[test]
fn stream_events_win_over_older_queries_and_phases_are_distinct() {
    let mut live = LiveState::default();
    let query = live.generation();
    assert_eq!(live.status(), Status::Unknown);
    live.event(Status::Live);
    let phase = live.phase();
    assert!(!live.query(query, false));
    assert_eq!(live.status(), Status::Live);
    live.event(Status::Live);
    assert_eq!(live.phase(), phase);
    live.event(Status::Offline);
    live.event(Status::Live);
    assert_ne!(live.phase(), phase);
    live.event(Status::Unknown);
    let query = live.generation();
    assert!(live.query(query, false));
    assert_eq!(live.status(), Status::Offline);
}
